// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Stamping a session's reward onto its ATIF trajectory.
//!
//! The scoring itself is pure and lives in [`sven_session_model::OutcomeFold`];
//! this module is only the ATIF-facing half — where the number goes on the
//! wire, and how to read it back.
//!
//! ## The wire contract
//!
//! An external trainer reads exactly one path off a serialized trajectory:
//!
//! ```text
//! trajectory.final_metrics.extra.reward     // a JSON number
//! ```
//!
//! Absent, non-numeric, or `null` means **outcome unknown**: the trajectory is
//! skipped, never defaulted. That is what makes conclusion-only stamping safe
//! — an incremental flush of a still-running session simply carries no reward
//! and is passed over, with no coordination needed between writer and reader.
//!
//! `final_metrics.extra` is schema-open JSON that ATIF explicitly reserves for
//! producer metrics outside the core schema, so this is not a format change;
//! `total_cache_write_tokens` (written by
//! [`crate::chat_usage_to_final_metrics`]) already lives there, which is why
//! stamping merges into `extra` rather than replacing it.

use atif::{FinalMetrics, Trajectory};
use serde_json::{Map, Value};

pub use sven_session_model::{OutcomeFold, RunConclusion, SessionReward};

/// The key the reward is read from, inside `final_metrics.extra`.
pub const REWARD_KEY: &str = "reward";

/// Insert (or replace) `reward`, `outcome` and the tool counters inside
/// `trajectory.final_metrics.extra`, creating `final_metrics` and/or `extra`
/// if absent and coercing a non-object `extra` to an object.
///
/// Sibling keys already under `extra` are preserved — mirrors
/// [`crate::SvenSessionMeta::apply_to_trajectory`], which does the same for
/// `Trajectory.extra`.
///
/// Call this **only when a session has concluded**: a stamped trajectory
/// asserts "this outcome is final".
pub fn apply_reward_to_trajectory(trajectory: &mut Trajectory, reward: &SessionReward) {
    let metrics = trajectory
        .final_metrics
        .get_or_insert_with(FinalMetrics::default);
    let extra = metrics
        .extra
        .get_or_insert_with(|| Value::Object(Map::new()));
    if !extra.is_object() {
        *extra = Value::Object(Map::new());
    }
    let obj = extra.as_object_mut().expect("just ensured object");
    obj.insert(REWARD_KEY.to_string(), Value::from(reward.reward));
    obj.insert("outcome".to_string(), Value::from(reward.outcome));
    obj.insert("tool_calls".to_string(), Value::from(reward.tool_calls));
    obj.insert("tool_errors".to_string(), Value::from(reward.tool_errors));
}

/// Read the stamped reward back, using the same lookup the external trainer
/// does. `None` means "this session's outcome is unknown" — never `Some(0.0)`.
pub fn trajectory_reward(trajectory: &Trajectory) -> Option<f64> {
    trajectory
        .final_metrics
        .as_ref()?
        .extra
        .as_ref()?
        .get(REWARD_KEY)?
        .as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_document::ChatUsage;
    use crate::trace_session::{
        chat_usage_to_final_metrics, default_agent_profile, final_metrics_to_chat_usage,
        ATIF_SCHEMA_VERSION,
    };

    fn trajectory() -> Trajectory {
        Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile())
    }

    fn reward(value: f64) -> SessionReward {
        SessionReward {
            reward: value,
            outcome: "success",
            tool_calls: 3,
            tool_errors: 0,
        }
    }

    #[test]
    fn stamp_creates_final_metrics_when_absent() {
        // The headless runner never sets `final_metrics` at all, so the stamp
        // has to be able to conjure the whole nest.
        let mut t = trajectory();
        assert!(t.final_metrics.is_none());
        apply_reward_to_trajectory(&mut t, &reward(1.0));
        assert_eq!(trajectory_reward(&t), Some(1.0));
    }

    #[test]
    fn stamp_preserves_existing_extra_keys() {
        let usage = ChatUsage {
            total_input_tokens: 100,
            total_output_tokens: 20,
            total_cache_read_tokens: 5,
            total_cache_write_tokens: 7,
            total_cost_usd: 0.5,
        };
        let mut t = trajectory();
        t.final_metrics = Some(chat_usage_to_final_metrics(&usage));

        apply_reward_to_trajectory(&mut t, &reward(0.75));

        let metrics = t.final_metrics.as_ref().expect("metrics");
        assert_eq!(trajectory_reward(&t), Some(0.75));
        // The pre-existing producer metric must survive the merge, and the
        // usage round trip must be unaffected by the new sibling keys.
        assert_eq!(final_metrics_to_chat_usage(metrics), usage);
    }

    #[test]
    fn stamp_preserves_native_final_metrics_fields() {
        let mut t = trajectory();
        t.final_metrics = Some(FinalMetrics {
            total_prompt_tokens: Some(1120),
            total_cost_usd: Some(0.25),
            total_steps: Some(3),
            ..Default::default()
        });
        apply_reward_to_trajectory(&mut t, &reward(1.0));
        let m = t.final_metrics.as_ref().expect("metrics");
        assert_eq!(m.total_prompt_tokens, Some(1120));
        assert_eq!(m.total_cost_usd, Some(0.25));
        assert_eq!(m.total_steps, Some(3));
    }

    #[test]
    fn stamp_coerces_non_object_extra() {
        let mut t = trajectory();
        t.final_metrics = Some(FinalMetrics {
            extra: Some(Value::from("garbage")),
            ..Default::default()
        });
        apply_reward_to_trajectory(&mut t, &reward(0.5));
        assert_eq!(trajectory_reward(&t), Some(0.5));
    }

    #[test]
    fn stamp_is_idempotent_and_last_write_wins() {
        let mut t = trajectory();
        apply_reward_to_trajectory(&mut t, &reward(1.0));
        apply_reward_to_trajectory(&mut t, &reward(0.5));
        let extra = t
            .final_metrics
            .as_ref()
            .and_then(|m| m.extra.as_ref())
            .and_then(|e| e.as_object())
            .expect("extra object");
        assert_eq!(extra.get(REWARD_KEY).and_then(Value::as_f64), Some(0.5));
        assert_eq!(extra.keys().filter(|k| *k == REWARD_KEY).count(), 1);
    }

    #[test]
    fn brain_contract_read_path() {
        // The contract test: navigate the serialized JSON exactly the way the
        // external trainer does, with no `atif` types on the read side. This
        // is what fails if a field is renamed or the number goes non-finite
        // (which serializes as `null` and reads back as "no reward").
        let mut t = trajectory();
        let mut fold = OutcomeFold::default();
        fold.observe(&sven_vocab::SessionEvent::TurnComplete);
        apply_reward_to_trajectory(&mut t, &fold.conclude(RunConclusion::Success));

        let json = serde_json::to_string(&t).expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("parse");
        let r = value
            .get("final_metrics")
            .and_then(|m| m.get("extra"))
            .and_then(|e| e.get("reward"))
            .and_then(Value::as_f64);
        assert_eq!(r, Some(1.0));
        assert_eq!(
            value["final_metrics"]["extra"]["outcome"].as_str(),
            Some("success")
        );
    }

    #[test]
    fn unstamped_trajectory_reads_as_none() {
        let mut t = trajectory();
        assert_eq!(trajectory_reward(&t), None);
        // `final_metrics` present but carrying only usage is still "unknown".
        t.final_metrics = Some(FinalMetrics {
            total_prompt_tokens: Some(10),
            ..Default::default()
        });
        assert_eq!(trajectory_reward(&t), None);
    }

    #[test]
    fn reward_round_trips_through_write_and_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.json");
        let mut t = trajectory();
        t.session_id = Some("s1".to_string());
        apply_reward_to_trajectory(&mut t, &reward(0.9));

        atif::persist::write_trajectory_atomic(&path, &t, None).expect("write");
        let loaded = crate::trace_session::load_session_from(&path).expect("load");
        assert_eq!(trajectory_reward(&loaded), Some(0.9));

        // `final_metrics` precedes `steps` on the wire, so the cheap
        // header-only reader sees the reward too.
        let header = atif::persist::read_trajectory_header(&path).expect("header");
        assert_eq!(
            header
                .final_metrics
                .as_ref()
                .and_then(|m| m.extra.as_ref())
                .and_then(|e| e.get("reward"))
                .and_then(Value::as_f64),
            Some(0.9)
        );
    }
}
